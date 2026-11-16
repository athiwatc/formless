# Remove one reviewed pending reference without touching files

`DetachReviewedReference` is an additive protocol-v1 request advertised by
`reviewed_reference_detach_v1`. It takes Session, the complete ordered
`expected_attachments` array, a required zero-based `index`, and a required
nonempty `operation_id` (at most 256 bytes, no control characters). Missing
inventory, index or operation identity is a decode error. The successful reply
is `{removed:true,session_id,index,operation_id}`. Older daemons may reject this
optional operation; callers must not substitute legacy `DetachFile`.

This supplies a missing safety boundary for required reference controls. Legacy
Detach uses an ID-or-path key, which can match another occurrence when records
or keys overlap. Here live Session validation, strict complete inventory
comparison, positional row lookup and deletion share one SQLite IMMEDIATE
transaction. The selected row must be a supported reference (`reference:true`,
bounded name/path, boolean `dir`, unsigned recorded bytes). Stored files,
out-of-range positions, deleted/unknown Sessions, changed ordering/metadata,
corrupt JSON and oversized inventories fail without deleting pending rows.

The operation deletes exactly one pending metadata occurrence. It does not
resolve, stat, open or remove its reference target, including nonexistent paths.
It does not call artifact removal or change artifact records/reference counts.
Identical references in the same Session, other pending files and other Sessions
remain. Directory references and file references share these semantics; this is
not directory/file deletion or a grant of Tool access. No Run, message, Queue
item or provider call is created.

Comparison uses decoded JSON semantics, preserving array order, duplicate
occurrences and unknown fields. It is not raw-text comparison or a durable
storage-row identity lease: identical metadata removed and re-added can match.
Expected metadata uses the reviewed-send 64-record / 64-KiB compact-record budget
after existing wire decoding. Current metadata uses the strict inventory reader's
64-record / 64-KiB stored-text budget. These limits do not provide a total
transport/process-memory or latency guarantee.

## Receipts and limitations

A synchronous connection-local cache serializes the claim, transaction and
receipt. Once a valid request obtains a cache slot, same Session/connection/
operation retries return its first transaction outcome, including failure,
instead of executing again. Changed arguments cannot recertify a claimed
operation; a new explicit review needs a new identity. Input-shape/identity and
capacity rejections happen before a claim, remove nothing and are not cached.
They are distinct from a failed claimed transaction; this distinction is not
permission for a native client to automatically retry an error. The
reply binds Session, selected index and operation, and does not assert that
the inventory remains unchanged afterward.

The cache retains at most 256 completed outcomes globally for ten minutes after
completion. It fails closed at capacity rather than evicting a live receipt.
Expired outcomes are removed on the next operation. Connection teardown removes
that connection's entries after queued work completes; direct internal callers
share the existing no-connection identity. This is bounded in-process duplicate
protection, not durable idempotency. After expiry, disconnect or daemon restart,
an old operation can execute again if the expected inventory has been restored.

A fully written request can commit even if its client never reads the reply.
Disconnect is not cancellation of an already accepted request. Future native
integration must retain Unknown for failed/lost/malformed receipts, block unsafe
repeat work, and never automatically replay on reconnect. An empty or newly read
inventory is not authoritative operation reconciliation. The current envelope
does not prove a failure happened before commit; no blanket safe-retry claim is
made. Receipts do not freeze file contents or prove that a user saw a review.

## Native integration still required

This increment adds the protocol/storage/daemon boundary and tests. No native
Remove control is exposed yet; existing Session details remains read-only
inspection, and normal Send/Steer confirmation is unchanged. Required follow-up
is to select a reference from the accepted complete inventory, display the exact
selected metadata, focus Cancel by default, and confirm with one operation UUID.
Controls and callbacks must bind Client, connection, form/read revision, index
and action, and recheck composition/modal ownership before dispatch.

After a valid receipt, retire the old snapshot rather than fabricating current
state. Unsupported peers hand off to TUI without legacy fallback. The native
worker must retain non-replayed unknown outcomes through physical reconnect
and prevent spent IDs from executing again for that Client lifetime. Native
Attach/drop/image paste, stored-file removal, external editor/reference addition,
Queue ownership and durable reconciliation remain required engineering.

## Validation and acceptance

Storage tests exercise duplicate positions and ID/path collisions, another
Session and artifact records, stale/malformed/deleted/corrupt inventories,
transaction rollback, a SQLite trigger failure and a separate SQLite writer.
The bounded 50 ms blocked-writer observation complements IMMEDIATE transaction
semantics; it is not exhaustive scheduler stress or a latency promise.
Daemon tests cover concurrent same-key calls, cached failure, capacity/cleanup,
invalid payloads, lost reply and receipt expiry. The expiry test explicitly shows
that restoring identical metadata cannot replay a live cached operation, while
the same operation can execute again after expiry. It does not hide that limit.

A private actual-IPC fixture stages a stored file and two identical directory
references using existing APIs. It removes the selected second reference,
replays the receipt without removing another, rejects stale inventory and a
stored-file selection, and verifies remaining metadata, artifact records, stored
body and reference-target sentinel are unchanged. There are no messages/Runs
and zero fake-provider POSTs. The artifact oracle uses a read-only connection
to the fixture's own database; it does not inject mutation results. Exact source,
commands, logs and hashes are frozen in `native-detach-checks.json` outside Git.

Queue and production streaming stay disabled/unavailable as previously gated.
This is not native/platform acceptance or complete attachment parity. Billing,
the unresolved foreground approval/prior uncertain action, native IME,
accessibility/clipboard/scaling/picker/filesystem/lifecycle, Windows/Linux
execution, performance, licensing/distribution and signing/install/update gates
remain. No native renderer, user Profile, paid provider, blocked hosted job,
billing change, publication or foreground action is used.
