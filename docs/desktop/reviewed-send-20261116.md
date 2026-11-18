# Atomic pending-metadata comparison before Send or Steer

`SendReviewedMessage` is an additive protocol-v1 operation advertised by
`reviewed_attachment_send_v1`. The caller supplies Session, exact message text,
SendMode, a required nonempty action identity (at most 256 bytes, no controls),
and the complete ordered `expected_attachments` array from the inventory read.
Omitting that array or action identity is a decoding error. An empty array is
an explicit expectation that nothing is pending, not permission to attach any
files staged by another client. Older daemons may reject the operation; a future
native caller must never silently substitute legacy `SendMessage`.

This closes a specific shared-state race needed for attachment/send review.
A reread followed by legacy Send does not protect the gap between those two
requests. Here the live Session check, strict inventory comparison and user
message append/attachment consumption share one SQLite IMMEDIATE transaction.
Idle sends perform forecast consent and Run admission in that same transaction.
Steering uses the same transaction for comparison and message append. A changed,
deleted, corrupt or oversized inventory fails before consuming it or appending
the message. A failure before transaction commit rolls back both message and consumption.
A post-commit Run-launch failure or lost reply can leave the message persisted;
an error is not a general proof that no write occurred, and must not trigger
an automatic new-key retry.
Run-finalization classification retains the existing admission lock.

Comparison uses decoded JSON values, preserves array order and duplicates,
and includes unknown fields. It does not compare raw JSON whitespace or object
key ordering. Identical metadata removed and re-added can match; this is not a
row-identity lease. Expected metadata is bounded after wire decoding to 64
records and 64 KiB total compact record JSON. Current stored metadata uses the
existing strict 64-record/64-KiB stored-text inventory reader. Neither limit is
a total wire-decoding or process-memory bound.

Explicit Queue is rejected even when idle and even with an empty inventory.
An active Run that can no longer accept steering, cancellation, or publication
would make legacy Send defer its message; this operation instead fails without
queueing, consuming or appending. Deferred Queue attachment ownership needs a
separate design: checking metadata now would not bind attachments consumed when
a queued message eventually drains. There is no fallback to legacy Queue.

The preview now advertises `reviewed_send_mode_v1`. It checks the requested
mode while holding the existing Run admission lock. Send requires an idle
Session. Steer requires an active Run that can still accept steering. A change
between idle and active after review rejects before message append, inventory
consumption or Run admission. The check binds the mode and current availability.
It does not bind a Steer to a particular Run identity if another active Run has
already replaced it. Legacy `SendMessage` keeps its existing behavior.

`reviewed_steer_run_v1` also advertises `ReadSteerReview` and `SendReviewedSteer`.
The first returns one live steerable Run and the complete pending inventory. The
second requires its Run ID and refuses an ended or replaced Run under the same
admission lock, before comparing and consuming metadata. Bound Steer has its own
receipt namespace. A legacy or mode-only receipt cannot acknowledge it. It
retains the existing interruption cut and steering closure checks. As with
existing steering, an Interrupt that races an already admitted append is
best effort. No schema change or new durable receipt is involved.

The operation shares existing detached Send execution and receipt retention:
one action key on one connection waits for/replays its first outcome, including
failure, for ten minutes after completion. In-flight claims do not expire.
Legacy and reviewed sends use separate receipt namespaces, so a matching legacy
key cannot manufacture a reviewed acknowledgement. Lost waiters do not cancel
claimed execution. Connection close removes receipts; restart/reconnect do not
provide durable idempotency. Re-review after a rejection requires a new action
identity. Direct internal handle callers share the existing no-connection scope.
The connection worker also applies interruption cuts to this new request.

This endpoint is not an authorization token, a new Tool grant, a proof that the
user saw a particular UI, or a freeze of mutable referenced contents. Reference
paths may change contents while their metadata matches. Normal forecast,
credential, Session trust, security and provider rules still apply. An accepted
send starts provider work under those rules. Validation rejection before the
append starts no Run; this is distinct from failures after transaction commit.

## Native workflow at this boundary commit

At boundary commit `a8e6bda` the production native composer was deliberately
unchanged. The successor [native confirmation contract](native-send-review-20261116.md)
documents its integration; the rest of this section records the boundary's
original required follow-up.
Its existing legacy Send/Steer/Queue path therefore does not yet provide this
attachment-review guarantee. Session details remains an explicit metadata
inspection, not mandatory send review. The operation must first receive source
review, then the native preparation/confirmation path must read and display the
complete inventory, bind it to the exact draft, Session, Profile, connection
and action, and submit this operation without fallback. It must retain unknown
write outcomes without replay, recheck IME/draft/modal ownership at confirmation,
and explain unsupported Queue until deferred attachment ownership is solved.
Attach/drop/image paste, safe Detach identity, external editor/reference controls
and complete required attachment parity remain unfinished.

## Validation and acceptance limits

Core tests cover complete matching, additions/removals, ordering, duplicates,
unknown fields, changed paths, malformed/oversized input, corrupt stored data,
deleted Sessions, cancellation, closed steering, Queue rejection, transactional
rollback, duplicate/lost waiters, receipt namespace separation and real framing.
A separate SQLite writer is held outside the compare/consume transaction; its
late attachment remains pending after the reviewed message commits. The bounded
50 ms blocked-writer observation complements SQLite's IMMEDIATE transaction
semantics; it is not a universal scheduler stress or latency guarantee.

A private IPC/fake-provider fixture rejects stale empty metadata after a peer
stages a file, then starts exactly one Run from fresh metadata, preserves it in
the user message, consumes it once, and replays the receipt without another
provider POST. It uses no user Profile, paid provider or native renderer.
Exact commands/results/hashes are frozen outside the source tree in
`reviewed-send-checks.json`; exploratory failures remain preserved separately.

Production streaming remains disabled. Native IME, accessibility, OS clipboard,
picker/filesystem/scaling, Windows/Linux execution, native performance and
licensing/distribution acceptance remain open. Hosted desktop CI is blocked by
account billing/spending restrictions; no blocked job is retried. The pending
Formless Sol Validation approval and prior uncertain foreground action are not
repeated or bypassed. This is development groundwork, not GPUI completion,
distribution clearance or merge approval.
