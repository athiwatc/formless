# Room lifecycle ledger (#638, source phase)

`RoomBudget` is a metadata-only SQLite authority, not a ciphertext queue. One
bounded `relay_transport_state` row contains native socket identities,
generations, CID lifecycle state, send grants/destination receipts, control
receipts, deadlines, bounded permit waiters and fragment sequence metadata.
It never stores a fragment body, handshake proof or RPC content. `sql.exec()`
updates that one row synchronously before a send effect is returned. Failed
writes leave the old in-memory authority intact and return no effects. The
Cloudflare normal output gate applies; `allowUnconfirmed` is never requested.
Malformed/unknown persisted state fails closed rather than minting fresh credit.

Sixteen CIDs include closing browsers and orphaned UP deliveries. Browser actual
close releases DOWN destination charges, but sent UP belongs to the daemon's
actual socket/generation until its receipt or actual teardown. A close request
releases no native-buffered sent charge. Hibernation rebuilds all credits from the
same row, not a mixture of live attachments and a second orphan cache. Native
attachments carry identity only. A room may have at most 18 tracked native
sockets including closing ones; adapters must refuse upgrades when that limit
or the 16 CID slots is reached.

Daemon-authenticated readiness alone does not grant data delivery. The daemon
and browser must each consume and ACK their own room-ready control, after local
E2E proof processing. Queued need metadata is bounded to eight per direction,
including granted entries, and round-robin draining preserves each CID/direction's
full-fragment reserve. Sender-visible grants cannot settle destination credit;
redemption creates a distinct 128-bit receipt. Socket identity is supplied by the
adapter, never chosen by the CID/generation payload.

Raw data reservations are still bounded by the shared transport's
`2*16*(64KiB+1024)+1MiB` policy, charged fourfold. Separate control reservations
are at most 128KiB raw, with a per-native 3072-byte progress reserve, shared
64KiB excess and 32 receipts/native. Metadata is at most 64KiB. These logical
retained-data limits fit the proposed 16MiB room policy; they are not an RSS,
garbage-collection or platform-owned incoming-queue guarantee. Adapters still
must enforce small pre-ready frames before parsing/encoding, fragment wire size,
control rate limits, native teardown semantics and periodic alarms. Endpoint
plaintext/encryption/decryption memory budgets remain a separate work stream.

The development adapter uses Node's built-in SQLite (`node:sqlite`, Node 22.13+
recommended) and the same SQL/state transitions, not an in-memory SQL parser.
Private tests exercise SQLite rehydration and failure paths without deployment,
credentials or provider traffic. The Worker/dev wire adapter is now connected to the frozen shared wire
contract, but this is not complete #638 acceptance without combined browser/
Rust endpoint and global producer/crypto memory proofs.

Official API sources retrieved 2026-11-01:

- [SQLite storage API](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/):
  synchronous SQL, single-write atomicity, transactionSync and normal output gates.
- [Alarms](https://developers.cloudflare.com/durable-objects/api/alarms/): one alarm,
  at-least-once execution; repeated handler execution must remain idempotent.
- [WebSocket hibernation](https://developers.cloudflare.com/durable-objects/best-practices/websockets/):
  attachments survive hibernation only while their connection remains healthy;
  source-close cannot preserve a daemon-owned receipt solely in that attachment.


The adapter preserves an already scheduled alarm during constructor hydration;
initialization uses the Durable Object concurrency barrier only for storage/
alarm reconstruction. A completed final fragment stays a logical-source barrier
until its destination receipt. Canonical tiny handshakes have a separate six-
message limit; at most two CID-origin controls are outstanding and other CIDs
retain a progress slot. Unsent lifecycle notices are metadata flags, not proof
or ciphertext queues, and are cleared atomically with their reserved receipt.
Their receipt also keeps a closed CID's slot alive until native delivery settles.
A daemon response racing a closed browser is a CID-local event; it cannot reset
healthy channels sharing the authenticated native connection.

The Node adapter uses bounded incremental native framing, fatal UTF-8 decoding,
128-fragment continuation limits, and a 4 KiB pending Ping/Pong/Close write cap
plus a native frame-rate cap. Only the actual socket close callback proves
native teardown. HTTP/push work is bounded separately: one push fanout (at most
ten stored subscriptions) and four subscription-control operations globally,
with at most one per native source. Subscription/unsubscription success replies
follow the real KV operation, never a guessed delay. No unbounded promise chain
or async KV write is used on the bulk credit path.


Receipt and assembly expiry first retires only that CID and sends its bounded
cancellation control; it does not close the shared daemon merely because one
final consumer stalled. The daemon can settle that closed CID's original
receipts after cancelling/dropping its buffers, without waiting on the shared
reader. A separate ten-second cancellation-control drain grace is enforced.
If the daemon consumes controls but a data receipt is still unknown, that fixed
orphan remains charged/admission-counted until its exact receipt or actual
native destination teardown. It is never silently refunded or used by itself
as a reason to reset healthy channels. A genuinely unconsumed shared control
past the separate grace can end the native connection; a close request still
provides no teardown proof. Input packet/byte rates are 8192 packets and
128 MiB per native socket per one-second window; CPU/HTTP ingress/RSS are not
claimed globally capped by those application counters.


Independent review hardening: Node sends an HTTP 429 before a capacity-rejected
WebSocket upgrade, rather than creating an untracked 101 connection. Accepted
native sockets get a one-second close grace, then the adapter destroys its own
socket if the peer keeps a half-open TCP side. Only the actual socket close
callback settles accounting; the grace timer never refunds credit by itself.
Error/requested closure retires logical device work immediately, while the CID
slot also remains counted for any still-present closing browser native socket,
even after cancellation/control/data receipts settle. Client-close controls
include the exact CID generation, and the daemon must match it rather than
infer a current actor.

Restoration validates daemon/client owner roles, active browser presence and
browser generation before publishing credits. If a live socket's attachment is
unknown or mismatched, it might be the very destination whose ledger ID appears
absent. The adapter closes that context, blocks new authority and holds all
unmatched charges until actual socket inventory is unambiguous. No refund is
made merely because identity could not be reconstructed. Native CLOSING state
cannot regain new-command authority after that inventory clears.
